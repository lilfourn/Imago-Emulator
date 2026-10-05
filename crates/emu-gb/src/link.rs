//! Two Game Boys joined by a link cable (Pan Docs, "Serial Data Transfer").
//!
//! Both machines run on one thread in alternating slices of 32 clock cycles
//! (8 M-cycles at normal speed). Between slices, a transfer started by one
//! machine's internal clock is delivered to the other, which answers with
//! its own byte if it is waiting on the external clock. A byte takes 1,024
//! M-cycles to send at the normal rate and 32 at the CGB's fast rate, so the
//! slice delay never shows. See `docs/decisions/0004-linked-cores.md`.

use emu_core::{
    Core, CoreInfo, Frame, Input, LoadError, Media, MemoryRegion, ScreenSize, StateError,
    StereoSample,
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
        })
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
        for (index, gb) in self.players.iter_mut().enumerate() {
            gb.set_input(&inputs.get(index).copied().unwrap_or_default());
        }
        let end = self.players[0].cycles() + CYCLES_PER_FRAME;
        while self.players[0].cycles() < end {
            let target = (self.players[0].cycles() + SLICE_CYCLES).min(end);
            for gb in &mut self.players {
                while gb.cycles() < target {
                    gb.step();
                }
            }
            self.service_cable();
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
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn reset(&mut self) {
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
        for (screen, gb) in self.screens.iter_mut().zip(&mut self.players) {
            gb.present();
            screen.clone_from(&gb.screens[0]);
        }
        Ok(())
    }

    fn backup_generation(&self, player: usize) -> Option<u64> {
        self.players.get(player)?.backup_generation(0)
    }

    fn save_backup(&self, player: usize, out: &mut Vec<u8>) {
        if let Some(gb) = self.players.get(player) {
            gb.save_backup(0, out);
        }
    }

    fn memory(&self, region: MemoryRegion) -> Option<&[u8]> {
        self.players[0].memory(region)
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
}
