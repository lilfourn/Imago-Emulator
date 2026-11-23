//! What the two-player cores share: two consoles joined by a link cable
//! or within wireless range, run as one [`Core`].
//!
//! How the two run together (the slices of time each console gets, and
//! what crosses the link between them) is each system's own and stays in
//! its crate. The plumbing around that is the same for every system and
//! is here: mixing the two consoles' sound into one stream, checking that
//! a saved pair's two clocks belong together, finding the cartridge a
//! battery save is asked of, and sending what a debugger asks about a CPU
//! to the console that CPU is in ([`Pair`]).

use crate::debug::{
    CpuInfo, Graphics, GraphicsRequest, Instruction, IoRegister, Register, RunOutcome,
    ScheduledEvent, Stops,
};
use crate::{Core, Debugger, Input, StateError, StereoSample};

/// Two consoles run as one core, for the debugger: a core that implements
/// this is a [`Debugger`] whose CPUs are player 0's console's and then
/// player 1's, each debugged as that console's own.
///
/// So with [`CPUS_EACH`](Self::CPUS_EACH) CPUs a console, the pair's CPU
/// `n` is CPU `n % CPUS_EACH` of console `n / CPUS_EACH`, and a number
/// past the last CPU is taken as one of player 1's.
pub trait Pair {
    /// One of the two consoles.
    type Console: Debugger;

    /// How many CPUs each console has: 1, or 2 for the DS's ARM9 and ARM7.
    const CPUS_EACH: usize;

    /// The two consoles, player 0's first.
    fn consoles(&self) -> &[Self::Console; 2];

    /// The two consoles, to change one.
    fn consoles_mut(&mut self) -> &mut [Self::Console; 2];

    /// [`Debugger::run_until`] for the pair: runs the rest of the frame on
    /// both consoles, with the pair's own slices, until it ends or one of
    /// `stops` applies. `stops` and the stop returned number CPUs as the
    /// pair does.
    fn run_pair_until(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        stops: &Stops,
    ) -> RunOutcome;
}

/// Which console the pair's CPU `cpu` is in, and its number there.
fn route<P: Pair + ?Sized>(cpu: usize) -> (usize, usize) {
    ((cpu / P::CPUS_EACH).min(1), cpu % P::CPUS_EACH)
}

/// The console the pair's CPU `cpu` is in, and its number there.
fn console<P: Pair + ?Sized>(pair: &P, cpu: usize) -> (&P::Console, usize) {
    let (player, cpu) = route::<P>(cpu);
    (&pair.consoles()[player], cpu)
}

impl<P: Pair> Debugger for P {
    fn cpus(&self) -> Vec<CpuInfo> {
        let mut cpus = Vec::new();
        for (player, console) in self.consoles().iter().enumerate() {
            for cpu in console.cpus() {
                cpus.push(CpuInfo { player, ..cpu });
            }
        }
        cpus
    }

    fn registers(&self, cpu: usize) -> Vec<Register> {
        let (console, cpu) = console(self, cpu);
        console.registers(cpu)
    }

    fn status(&self, cpu: usize) -> String {
        let (console, cpu) = console(self, cpu);
        console.status(cpu)
    }

    fn pc(&self, cpu: usize) -> u32 {
        let (console, cpu) = console(self, cpu);
        console.pc(cpu)
    }

    fn read(&self, cpu: usize, address: u32) -> Option<u8> {
        let (console, cpu) = console(self, cpu);
        console.read(cpu, address)
    }

    fn write(&mut self, cpu: usize, address: u32, value: u8) -> bool {
        let (player, cpu) = route::<P>(cpu);
        self.consoles_mut()[player].write(cpu, address, value)
    }

    fn disassemble(&self, cpu: usize, address: u32) -> Instruction {
        let (console, cpu) = console(self, cpu);
        console.disassemble(cpu, address)
    }

    fn io_registers(&self, cpu: usize) -> Vec<IoRegister> {
        let (console, cpu) = console(self, cpu);
        console.io_registers(cpu)
    }

    fn events(&self, cpu: usize) -> Vec<ScheduledEvent> {
        let (console, cpu) = console(self, cpu);
        console.events(cpu)
    }

    fn graphics(&self, cpu: usize, request: &GraphicsRequest) -> Option<Graphics> {
        let (console, cpu) = console(self, cpu);
        console.graphics(cpu, request)
    }

    fn run_until(
        &mut self,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        stops: &Stops,
    ) -> RunOutcome {
        self.run_pair_until(inputs, audio, stops)
    }
}

/// The furthest apart the two clocks of a pair's save state may be, in
/// the consoles' own cycles. Running slice by slice leaves them no more
/// than a slice and one step apart (an instruction, or a DMA), and the
/// console behind runs until it has caught up, so a state that put them
/// years apart would hold a frame as long.
const CLOCKS_APART_MOST: u64 = 1 << 24;

/// Checks that the clocks of the two consoles in a pair's save state, at
/// `first` and `second` cycles, are close enough to have been saved
/// together.
///
/// # Errors
///
/// Returns [`StateError::Malformed`] if they are not.
pub fn clocks_together(first: u64, second: u64) -> Result<(), StateError> {
    if first.abs_diff(second) <= CLOCKS_APART_MOST {
        Ok(())
    } else {
        Err(StateError::Malformed(
            "the linked state's two consoles are not at the same time",
        ))
    }
}

/// [`Core::backup_generation`] for a pair. Each console has one cartridge
/// of its own, so the pair's cartridge `i` is player `i`'s, which that
/// console knows as its cartridge 0; there is none past the second. (A
/// DS's GBA slot, its own cartridge 1, is out of a pair's reach.)
pub fn backup_generation<C: Core>(consoles: &[C; 2], cartridge: usize) -> Option<u64> {
    consoles.get(cartridge)?.backup_generation(0)
}

/// [`Core::save_backup`] for a pair, its cartridges numbered as for
/// [`backup_generation`]: appends nothing past the second.
pub fn save_backup<C: Core>(consoles: &[C; 2], cartridge: usize, out: &mut Vec<u8>) {
    if let Some(console) = consoles.get(cartridge) {
        console.save_backup(0, out);
    }
}

/// What [`mix`] does with the samples one console made and the other did
/// not, when a frame leaves them with different counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uneven {
    /// The mix ends with the shorter stream; the longer one's last samples
    /// are left out. Linked Game Boys and Game Boy Advances do this.
    Dropped,
    /// The longer stream plays to its end against silence. Linked DSes do
    /// this: a console whose power is off makes no sound at all.
    AgainstSilence,
}

/// Appends two consoles' sound to `out` as one stream: each sample is the
/// mean of the two consoles', channel by channel, rounded towards zero.
pub fn mix(
    first: &[StereoSample],
    second: &[StereoSample],
    uneven: Uneven,
    out: &mut Vec<StereoSample>,
) {
    let mean = |a: i16, b: i16| ((i32::from(a) + i32::from(b)) / 2) as i16;
    let length = match uneven {
        Uneven::Dropped => first.len().min(second.len()),
        Uneven::AgainstSilence => first.len().max(second.len()),
    };
    out.extend((0..length).map(|index| {
        let a = first.get(index).copied().unwrap_or_default();
        let b = second.get(index).copied().unwrap_or_default();
        [mean(a[0], b[0]), mean(a[1], b[1])]
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A console that answers with its own name and the CPU number it was
    /// asked about, and remembers what was written to it.
    #[derive(Default)]
    struct Fake {
        name: &'static str,
        cpus: usize,
        written: Vec<(usize, u32, u8)>,
    }

    impl Debugger for Fake {
        fn cpus(&self) -> Vec<CpuInfo> {
            let name = self.name;
            vec![CpuInfo { name, player: 0 }; self.cpus]
        }

        fn registers(&self, cpu: usize) -> Vec<Register> {
            vec![Register {
                name: self.name,
                value: cpu as u32,
                bits: 32,
            }]
        }

        fn status(&self, cpu: usize) -> String {
            format!("{} {cpu}", self.name)
        }

        fn pc(&self, cpu: usize) -> u32 {
            cpu as u32
        }

        fn read(&self, cpu: usize, address: u32) -> Option<u8> {
            (address == 0).then_some(cpu as u8)
        }

        fn write(&mut self, cpu: usize, address: u32, value: u8) -> bool {
            self.written.push((cpu, address, value));
            true
        }

        fn disassemble(&self, cpu: usize, address: u32) -> Instruction {
            Instruction {
                address,
                bytes: Vec::new(),
                text: format!("{} {cpu}", self.name),
                call: false,
            }
        }

        fn events(&self, cpu: usize) -> Vec<ScheduledEvent> {
            vec![ScheduledEvent {
                cycles: cpu as i64,
                name: self.name.to_string(),
            }]
        }

        fn run_until(&mut self, _: &[Input], _: &mut Vec<StereoSample>, _: &Stops) -> RunOutcome {
            unreachable!("a pair runs its consoles itself")
        }
    }

    /// A console whose one cartridge's battery save is this byte, written
    /// as many times. It has no second cartridge to be asked about.
    struct Cartridge(u8);

    impl Core for Cartridge {
        fn info(&self) -> crate::CoreInfo {
            unreachable!()
        }

        fn run_frame(&mut self, _: &[Input], _: &mut Vec<StereoSample>) {}

        fn screens(&self) -> &[crate::Frame] {
            &[]
        }

        fn reset(&mut self) {}

        fn save_state(&self, _: &mut Vec<u8>) {}

        fn load_state(&mut self, _: &[u8]) -> Result<(), StateError> {
            Ok(())
        }

        fn backup_generation(&self, cartridge: usize) -> Option<u64> {
            assert_eq!(cartridge, 0, "a console's own cartridge is its first");
            Some(u64::from(self.0))
        }

        fn save_backup(&self, cartridge: usize, out: &mut Vec<u8>) {
            assert_eq!(cartridge, 0, "a console's own cartridge is its first");
            out.push(self.0);
        }
    }

    #[test]
    fn a_pairs_cartridge_is_the_player_of_that_numbers_own() {
        let consoles = [Cartridge(5), Cartridge(9)];
        assert_eq!(backup_generation(&consoles, 0), Some(5));
        assert_eq!(backup_generation(&consoles, 1), Some(9));
        assert_eq!(backup_generation(&consoles, 2), None);

        let mut saves = vec![1];
        save_backup(&consoles, 1, &mut saves);
        save_backup(&consoles, 0, &mut saves);
        save_backup(&consoles, 2, &mut saves);
        assert_eq!(saves, [1, 9, 5], "appended, and nothing past the second");
    }

    /// Two [`Fake`]s with `EACH` CPUs each.
    struct Two<const EACH: usize>([Fake; 2]);

    impl<const EACH: usize> Two<EACH> {
        fn new() -> Self {
            Self(["first", "second"].map(|name| Fake {
                name,
                cpus: EACH,
                written: Vec::new(),
            }))
        }
    }

    impl<const EACH: usize> Pair for Two<EACH> {
        type Console = Fake;
        const CPUS_EACH: usize = EACH;

        fn consoles(&self) -> &[Fake; 2] {
            &self.0
        }

        fn consoles_mut(&mut self) -> &mut [Fake; 2] {
            &mut self.0
        }

        fn run_pair_until(
            &mut self,
            _: &[Input],
            _: &mut Vec<StereoSample>,
            _: &Stops,
        ) -> RunOutcome {
            unreachable!("the routing tests never run the pair")
        }
    }

    #[test]
    fn a_pairs_cpus_are_the_first_consoles_then_the_seconds() {
        let cpus = |cpus: Vec<CpuInfo>| -> Vec<(&str, usize)> {
            cpus.iter().map(|cpu| (cpu.name, cpu.player)).collect()
        };
        assert_eq!(cpus(Two::<1>::new().cpus()), [("first", 0), ("second", 1)]);
        assert_eq!(
            cpus(Two::<2>::new().cpus()),
            [("first", 0), ("first", 0), ("second", 1), ("second", 1)]
        );
    }

    #[test]
    fn what_a_debugger_asks_about_a_cpu_goes_to_its_console_under_its_own_number() {
        // One CPU a console: CPU n is player n's CPU 0, and anything past
        // the end is player 1's.
        let one = Two::<1>::new();
        assert_eq!(one.status(0), "first 0");
        assert_eq!(one.status(1), "second 0");
        assert_eq!(one.status(7), "second 0");

        // Two CPUs a console, as on the DS.
        let mut two = Two::<2>::new();
        assert_eq!(two.status(1), "first 1");
        assert_eq!(two.status(2), "second 0");
        assert_eq!(two.status(3), "second 1");
        assert_eq!(two.status(5), "second 1", "past the end");
        assert_eq!(two.registers(3)[0].value, 1);
        assert_eq!(two.pc(3), 1);
        assert_eq!(two.read(3, 0), Some(1));
        assert_eq!(two.disassemble(2, 8).text, "second 0");
        assert_eq!(two.events(1)[0].cycles, 1);
        assert_eq!(two.events(2)[0].name, "second");

        assert!(two.write(3, 0x10, 0xAB));
        assert!(two.write(0, 0x20, 0xCD));
        assert_eq!(two.0[0].written, [(0, 0x20, 0xCD)]);
        assert_eq!(two.0[1].written, [(1, 0x10, 0xAB)]);
    }

    #[test]
    fn a_pairs_clocks_may_be_a_little_apart_either_way() {
        assert_eq!(clocks_together(1_000, 1_000), Ok(()));
        assert_eq!(clocks_together(0, 1 << 24), Ok(()));
        assert_eq!(clocks_together(u64::MAX, u64::MAX - (1 << 24)), Ok(()));
        let apart =
            StateError::Malformed("the linked state's two consoles are not at the same time");
        assert_eq!(clocks_together(0, (1 << 24) + 1), Err(apart.clone()));
        assert_eq!(clocks_together(u64::MAX, 0), Err(apart));
    }

    #[test]
    fn a_mix_is_the_mean_of_both_consoles_rounded_towards_zero() {
        let mut out = vec![[7, 7]];
        mix(
            &[[100, -100], [i16::MAX, i16::MIN]],
            &[[50, -51], [i16::MAX, i16::MIN]],
            Uneven::Dropped,
            &mut out,
        );
        assert_eq!(out, [[7, 7], [75, -75], [i16::MAX, i16::MIN]]);
    }

    #[test]
    fn an_uneven_mix_drops_the_extra_samples_or_plays_them_against_silence() {
        let (long, short) = ([[10, 20], [30, 40], [-50, 61]], [[10, 20]]);
        for (first, second) in [(&long[..], &short[..]), (&short[..], &long[..])] {
            let mut dropped = Vec::new();
            mix(first, second, Uneven::Dropped, &mut dropped);
            assert_eq!(dropped, [[10, 20]]);

            let mut padded = Vec::new();
            mix(first, second, Uneven::AgainstSilence, &mut padded);
            assert_eq!(padded, [[10, 20], [15, 20], [-25, 30]]);
        }
    }
}
