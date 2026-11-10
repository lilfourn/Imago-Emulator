//! Two Game Boy Advances joined by a link cable (GBATEK, "GBA Communication
//! Ports").
//!
//! Both consoles run on one thread in alternating slices of 128 cycles. The
//! moment either starts a transfer, the cable delivers it: a multi-player
//! transfer collects both consoles' words, and a normal one swaps data with
//! a partner waiting on the external clock. Each side then finishes after
//! the transfer's length in its own time. See
//! `docs/decisions/0004-linked-cores.md`.

use emu_core::debug::{
    CpuInfo, Graphics, GraphicsRequest, Instruction, IoRegister, Register, RunOutcome,
    ScheduledEvent, Stop, Stops,
};
use emu_core::{
    Core, CoreInfo, Debugger, Frame, Input, LoadError, Media, MemoryRegion, RomPatch, ScreenSize,
    StateError, StereoSample,
};

use emu_state::payload;

use crate::sio::Outgoing;
use crate::{CYCLES_PER_FRAME, Gba, INFO, Machine};

/// Cycles each console runs before the other catches up.
const SLICE_CYCLES: u64 = 128;

/// The furthest apart a state's two clocks may be. A slice leaves them
/// an instruction or a DMA apart, and the console behind runs until it
/// has caught up, so a state that put them years apart would hold a
/// frame as long.
const CLOCKS_APART_MOST: u64 = 1 << 24;

const SCREENS: [ScreenSize; 2] = [ScreenSize::new(240, 160), ScreenSize::new(240, 160)];

/// Two Game Boy Advances and the cable between them, as one two-player
/// core. Player 0 is the parent (the cable's purple end).
#[derive(Debug)]
pub struct Linked {
    players: [Gba; 2],
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
    /// Powers on two Game Boy Advances, one per entry of `media`.
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
        let mut linked = Self {
            players: [Gba::new(first)?, Gba::new(second)?],
            screens: [Frame::new(SCREENS[0]), Frame::new(SCREENS[1])],
            resume: None,
        };
        linked.plug_in();
        Ok(linked)
    }

    fn plug_in(&mut self) {
        for (position, gba) in (0u8..).zip(&mut self.players) {
            gba.machine.bus.sio.connect(position, 2);
        }
    }

    /// Updates the cable's ready line and delivers started transfers.
    fn service_cable(&mut self) {
        let [a, b] = &mut self.players;
        let ready = a.machine.bus.sio.in_multi_mode() && b.machine.bus.sio.in_multi_mode();
        a.machine.bus.sio.set_ready(ready);
        b.machine.bus.sio.set_ready(ready);
        deliver(a, b);
        deliver(b, a);
    }

    fn present(&mut self) {
        for (screen, gba) in self.screens.iter_mut().zip(&self.players) {
            screen.clone_from(&gba.screens[0]);
        }
    }

    /// Runs both consoles through exactly one frame's time, so every call
    /// ends at the same point of the video frame, each step by `step`
    /// (given the console and its player number), until the frame ends or
    /// a step returns a stop. After a stop the next call carries on where
    /// this one left off.
    fn run(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        mut step: impl FnMut(&mut Gba, usize) -> Option<Stop>,
    ) -> RunOutcome {
        for (index, gba) in self.players.iter_mut().enumerate() {
            gba.set_input(&inputs.get(index).copied().unwrap_or_default());
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
                    let stop = step(&mut self.players[index], index);
                    if self.players[index].machine.bus.sio.has_outgoing() {
                        self.service_cable();
                    }
                    if stop.is_some() {
                        self.resume = Some(Resume { end, target });
                        return RunOutcome {
                            frame_done: false,
                            stop,
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
        self.present();

        // Both consoles produce the same number of samples; play them mixed.
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

    /// The console CPU `cpu` (also its player number) is in.
    fn player(&self, cpu: usize) -> &Gba {
        &self.players[cpu.min(1)]
    }
}

/// Delivers a transfer `from` started to `to`, and `to`'s side back.
fn deliver(from: &mut Gba, to: &mut Gba) {
    let Some(outgoing) = from.machine.bus.sio.take_outgoing() else {
        return;
    };
    match outgoing {
        Outgoing::Multi { word, cycles } => {
            let child = to.machine.bus.sio.multi_word().unwrap_or(0xFFFF);
            let words = [word, child, 0xFFFF, 0xFFFF];
            from.machine.bus.sio.set_incoming(words);
            if to.machine.bus.sio.join_multi(words) {
                to.machine.bus.schedule_serial_done(cycles);
            }
        }
        Outgoing::Normal {
            value,
            bits,
            cycles,
        } => {
            if let Some(reply) = to.machine.bus.sio.clocked_by_partner(value, bits) {
                from.machine.bus.sio.set_incoming([
                    reply as u16,
                    (reply >> 16) as u16,
                    0xFFFF,
                    0xFFFF,
                ]);
                to.machine.bus.schedule_serial_done(cycles);
            }
        }
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
        self.run(inputs, audio, |gba, _| {
            gba.step();
            None
        });
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn reset(&mut self) {
        self.resume = None;
        for gba in &mut self.players {
            gba.reset();
        }
        self.plug_in();
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        let machines = (&self.players[0].machine, &self.players[1].machine);
        payload::append(&machines, out);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let (first, second): (Machine, Machine) =
            payload::load(state, "not a linked Game Boy Advance state")?;
        // Both are checked before either console is changed.
        first.bus.fits()?;
        second.bus.fits()?;
        payload::check(
            first.bus.cycles().abs_diff(second.bus.cycles()) <= CLOCKS_APART_MOST,
            "the linked state's two consoles are not at the same time",
        )?;
        for (gba, mut machine) in self.players.iter_mut().zip([first, second]) {
            machine.bus.take_media_from(&mut gba.machine.bus);
            gba.machine = machine;
            gba.present();
        }
        self.resume = None;
        self.present();
        Ok(())
    }

    /// Each player has one cartridge: cartridge `i` is player `i`'s.
    fn backup_generation(&self, cartridge: usize) -> Option<u64> {
        self.players.get(cartridge)?.backup_generation(0)
    }

    fn save_backup(&self, cartridge: usize, out: &mut Vec<u8>) {
        if let Some(gba) = self.players.get(cartridge) {
            gba.save_backup(0, out);
        }
    }

    fn peek(&self, player: usize, address: u32) -> Option<u8> {
        self.players.get(player)?.peek(0, address)
    }

    fn poke(&mut self, player: usize, address: u32, value: u8) -> bool {
        self.players
            .get_mut(player)
            .is_some_and(|gba| gba.poke(0, address, value))
    }

    fn poke_io_16(&mut self, player: usize, address: u32, value: u16) -> bool {
        self.players
            .get_mut(player)
            .is_some_and(|gba| gba.poke_io_16(0, address, value))
    }

    fn set_rom_patches(&mut self, player: usize, patches: &[RomPatch]) {
        if let Some(gba) = self.players.get_mut(player) {
            gba.set_rom_patches(0, patches);
        }
    }

    fn memory(&self, region: MemoryRegion) -> Option<&[u8]> {
        self.players[0].memory(region)
    }

    fn debugger(&mut self) -> Option<&mut dyn Debugger> {
        Some(self)
    }
}

/// CPU `i` is player `i`'s; each is debugged as a lone console's.
impl Debugger for Linked {
    fn cpus(&self) -> Vec<CpuInfo> {
        (0..self.players.len())
            .map(|player| CpuInfo {
                name: "ARM7TDMI",
                player,
            })
            .collect()
    }

    fn registers(&self, cpu: usize) -> Vec<Register> {
        self.player(cpu).registers(0)
    }

    fn status(&self, cpu: usize) -> String {
        self.player(cpu).status(0)
    }

    fn pc(&self, cpu: usize) -> u32 {
        self.player(cpu).pc(0)
    }

    fn read(&self, cpu: usize, address: u32) -> Option<u8> {
        self.player(cpu).read(0, address)
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

    fn events(&self, cpu: usize) -> Vec<ScheduledEvent> {
        self.player(cpu).events(0)
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
        self.run(inputs, audio, |gba, cpu| gba.step_checked(cpu, stops))
    }
}

#[cfg(test)]
mod tests {
    use emu_arm::{Access, Bus as _};

    use super::*;

    /// A cartridge that loops forever with `B .`.
    fn rom() -> Vec<u8> {
        let mut rom = vec![0; 0x200];
        rom[..4].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
        rom[0xB2] = 0x96;
        rom
    }

    fn write(gba: &mut Gba, address: u32, value: u16) {
        gba.machine
            .bus
            .write_16(address, value, Access::NONSEQUENTIAL);
    }

    fn read(gba: &mut Gba, address: u32) -> u16 {
        gba.machine.bus.read_16(address, Access::NONSEQUENTIAL)
    }

    fn linked() -> Linked {
        let rom = rom();
        Linked::new(&[Media::new(&rom), Media::new(&rom)]).expect("valid roms")
    }

    #[test]
    fn a_breakpoint_stops_one_player_and_the_frame_resumes_in_step() {
        use emu_core::debug::{Breakpoint, StopReason};

        let (mut plain, mut debugged) = (linked(), linked());
        let stops = Stops {
            breakpoints: vec![Breakpoint {
                cpu: 1,
                address: 0x0800_0000,
            }],
            ..Stops::default()
        };
        let outcome = debugged.run_until(&[], &mut Vec::new(), &stops);
        let stop = Stop {
            cpu: 1,
            reason: StopReason::Breakpoint,
        };
        assert_eq!(outcome.stop, Some(stop));
        assert!(debugged.players[0].cycles() > 0, "player 0 ran its slice");
        // Without stops the rest of the frame runs on the same slices.
        debugged.run_frame(&[], &mut Vec::new());
        plain.run_frame(&[], &mut Vec::new());
        let (mut a, mut b) = (Vec::new(), Vec::new());
        plain.save_state(&mut a);
        debugged.save_state(&mut b);
        assert!(a == b, "the stop changed the emulation");
    }

    #[test]
    fn multi_player_transfers_reach_both_consoles() {
        let mut linked = linked();
        for (gba, word) in linked.players.iter_mut().zip([0x1111, 0x2222]) {
            write(gba, 0x0400_0134, 0x0000); // RCNT: serial
            write(gba, 0x0400_0128, 0x6003); // multi-player, 115200, IRQ
            write(gba, 0x0400_012A, word);
        }
        linked.service_cable();
        let [parent, child] = &mut linked.players;
        assert_eq!(read(parent, 0x0400_0128) & 0x7C, 0b00_1000); // SI low, SD
        assert_eq!(read(child, 0x0400_0128) & 0x7C, 0b01_1100); // SI high, SD, ID 1

        write(parent, 0x0400_0128, 0x6083);
        linked.run_frame(&[], &mut Vec::new());
        for gba in &mut linked.players {
            let words = [0x120, 0x122, 0x124, 0x126].map(|offset| read(gba, 0x0400_0000 + offset));
            assert_eq!(words, [0x1111, 0x2222, 0xFFFF, 0xFFFF]);
            assert_eq!(read(gba, 0x0400_0128) & 0x80, 0, "transfer done");
            assert_ne!(read(gba, 0x0400_0202) & 0x80, 0, "serial interrupt");
        }
    }

    #[test]
    fn normal_transfers_swap_data() {
        let mut linked = linked();
        let [master, slave] = &mut linked.players;
        for gba in [&mut *master, &mut *slave] {
            write(gba, 0x0400_0134, 0x0000);
        }
        write(slave, 0x0400_0120, 0x5678);
        write(slave, 0x0400_0122, 0x1234);
        write(slave, 0x0400_0128, 0x1080); // 32-bit, external clock, start
        write(master, 0x0400_0120, 0xBEEF);
        write(master, 0x0400_0122, 0xDEAD);
        write(master, 0x0400_0128, 0x1081); // 32-bit, internal clock, start
        linked.run_frame(&[], &mut Vec::new());
        let [master, slave] = &mut linked.players;
        assert_eq!(
            (read(master, 0x0400_0120), read(master, 0x0400_0122)),
            (0x5678, 0x1234)
        );
        assert_eq!(
            (read(slave, 0x0400_0120), read(slave, 0x0400_0122)),
            (0xBEEF, 0xDEAD)
        );
    }

    #[test]
    fn a_linked_frame_takes_one_frame_of_cycles() {
        let mut linked = linked();
        linked.run_frame(&[], &mut Vec::new());
        let start = linked.players.each_ref().map(Gba::cycles);
        linked.run_frame(&[], &mut Vec::new());
        for (gba, start) in linked.players.iter().zip(start) {
            let length = gba.cycles() - start;
            assert!(
                (CYCLES_PER_FRAME..CYCLES_PER_FRAME + 64).contains(&length),
                "{length}"
            );
        }
    }

    #[test]
    fn rom_patches_reach_one_player_and_outlast_linked_states() {
        let mut linked = linked();
        let mut state = Vec::new();
        linked.save_state(&mut state);
        let patch = RomPatch {
            address: 0x0800_0000,
            value: 0x55,
            compare: None,
        };
        linked.set_rom_patches(1, &[patch]);
        linked.load_state(&state).expect("own state");
        let [first, second] = &mut linked.players;
        assert_eq!(read(first, 0x0800_0000), 0xFFFE);
        assert_eq!(read(second, 0x0800_0000), 0xFF55);
    }

    #[test]
    fn cheat_io_writes_reach_one_player() {
        let mut linked = linked();
        assert!(linked.poke_io_16(1, 0x0400_0050, 0x00FF));
        assert!(!linked.poke_io_16(2, 0x0400_0050, 0x00FF));
        let [first, second] = &mut linked.players;
        assert_eq!(read(first, 0x0400_0050), 0);
        assert_eq!(read(second, 0x0400_0050), 0x00FF);
    }
}
